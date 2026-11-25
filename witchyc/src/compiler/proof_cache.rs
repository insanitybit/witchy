//! Proofs the selected checker has already accepted.
//!
//! An entry says: this exact Lean text (imports, prelude, statement and proof script of one
//! goal) passed the kernel and the axiom audit under this checker binding. The key is the
//! hash of all of that, so a changed statement, script, prelude, helper or SDK record is a
//! miss. A hit skips Lean for that goal. The cache directory is trusted like the compiler:
//! whoever can write it can claim a proof. It is used only when the operator names one.
//!
//! So that "whoever can write it" is the user running the compiler and nobody else, a directory
//! is used only if that user owns it, neither its group nor others can write it, and no part of
//! its path is a symbolic link; and each file read or added to in it (`accepted.v1`,
//! `refused.v1`, an earlier-layout entry, a lock) must be a regular file with the same owner and
//! modes, not a link. On macOS none of them may carry an access control list, and each directory
//! above the cache belongs to root or the user and is writable by nobody else unless sticky.
//! Anything else refuses the build (review 2026-11-21: a writable directory
//! let anyone plant an `accepted.v1` line and have a false lemma accepted). The directory is
//! opened once, one path component at a time without following links, the checks are made on
//! the open handles, and every file is opened relative to the directory's handle and checked
//! on the handle that is then read or written: nothing is checked by name and then used by name.
//!
//! A goal Lean refused, with the message it gave, is kept too (`refused.v1`), and so is what a
//! refused build's explanation found. Using one can only refuse a program, never admit one, so
//! that file is not trusted the way the accepted one is. Only a refusal Lean attributed to a
//! goal is kept: a request that failed without naming one (a crash, a time limit) is not.
//!
//! Entries are lines of one file, read once per build: opening a file per goal was 40% of a
//! cached build's time. A directory written by an earlier compiler has a file per entry
//! instead; those are still read, and each one found is copied into the one file.
use super::{proof::Goal, proof_digest as digest};
use crate::diagnostic::Diagnostic;
use std::{cell::RefCell, collections::HashMap, fs, io::{Read, Write}, path::Path};
type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: &'static str) -> Diagnostic { Diagnostic::new("W0402", 0..0, message) }
const SCHEMA: &str = "witchy.proof-cache.v1";
/// One line for each accepted goal: its key, a space, `axioms=` and the names the audit found.
const ACCEPTED: &str = "accepted.v1";
pub(super) const AXIOMS: [&str; 3] = ["propext", "Classical.choice", "Quot.sound"];
const REFUSED: &str = "refused.v1";
/// A goal's standing in the cache.
pub(super) enum Found { Refused(String), Accepted(String), Missing }
/// How a file of the cache is opened: to read it, to add whole lines to it (made if missing), or
/// as a lock (made if missing).
#[derive(Clone, Copy)]
enum Access { Read, Append, Lock }
const NOT_FOUND: &str = "WITCHY_PROOF_CACHE must name an existing absolute directory";
const LINKED: &str = "WITCHY_PROOF_CACHE is refused: part of the directory's path is a symbolic link (name the directory by its real path)";
const NOT_OWNED: &str = "WITCHY_PROOF_CACHE is refused: the directory is not owned by the user running the compiler, and whoever can write it can claim a proof";
const WRITABLE: &str = "WITCHY_PROOF_CACHE is refused: the directory is writable by its group or by others, and whoever can write it can claim a proof (`chmod go-w`)";
const FILE_LINKED: &str = "the proof cache is refused: one of its files is a symbolic link or not a regular file";
const FILE_NOT_OWNED: &str = "the proof cache is refused: one of its files is not owned by the user running the compiler";
const FILE_WRITABLE: &str = "the proof cache is refused: one of its files is writable by its group or by others (`chmod go-w`)";
const LISTED: &str = "WITCHY_PROOF_CACHE is refused: the directory carries an access control list, which can let another user write it whatever its modes say (`chmod -N`)";
const FILE_LISTED: &str = "the proof cache is refused: one of its files carries an access control list (`chmod -N`)";
const ABOVE: &str = "WITCHY_PROOF_CACHE is refused: a directory above it belongs to another user, or others can write it and it is not sticky, so the directory could be replaced";
const FILE_UNREADABLE: &str = "the proof cache is refused: one of its files cannot be opened";
/// The cache directory, open. Everything in it is reached through this handle.
struct Directory { handle: fs::File, owner: u32 }
#[cfg(all(unix, any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))))]
mod handle {
    use super::*;
    use std::{ffi::{CString, c_char, c_int, c_uint}, os::unix::{ffi::OsStrExt, fs::MetadataExt, io::{AsRawFd, FromRawFd}}, path::Component};
    #[cfg(target_os = "macos")]
    mod flag { pub const WRITE: i32 = 1; pub const READ_WRITE: i32 = 2; pub const NONBLOCK: i32 = 0x4; pub const APPEND: i32 = 0x8; pub const NOFOLLOW: i32 = 0x100; pub const CREATE: i32 = 0x200; pub const DIRECTORY: i32 = 0x10_0000; pub const CLOEXEC: i32 = 0x100_0000; pub const ELOOP: i32 = 62; }
    #[cfg(target_os = "linux")]
    mod flag {
        pub const WRITE: i32 = 1; pub const READ_WRITE: i32 = 2; pub const CREATE: i32 = 0o100; pub const APPEND: i32 = 0o2000; pub const NONBLOCK: i32 = 0o4000; pub const CLOEXEC: i32 = 0o2000000; pub const ELOOP: i32 = 40;
        #[cfg(target_arch = "x86_64")] pub const DIRECTORY: i32 = 0o200000;
        #[cfg(target_arch = "x86_64")] pub const NOFOLLOW: i32 = 0o400000;
        #[cfg(target_arch = "aarch64")] pub const DIRECTORY: i32 = 0o40000;
        #[cfg(target_arch = "aarch64")] pub const NOFOLLOW: i32 = 0o100000;
    }
    #[cfg(target_os = "macos")] type Mode = u16;
    #[cfg(not(target_os = "macos"))] type Mode = u32;
    unsafe extern "C" {
        fn openat(directory: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
        fn mkdirat(directory: c_int, path: *const c_char, mode: Mode) -> c_int;
        fn geteuid() -> u32;
    }
    /// macOS: an extended access control list can grant another user `write`, `add_file` or
    /// `delete_child` without touching the mode bits, so a handle that carries one at all is
    /// refused, and so is one whose list cannot be read. (On Linux a POSIX ACL shows in the group
    /// bits, which hold its mask: no named entry can write when the group bits cannot.)
    #[cfg(target_os = "macos")]
    fn listed(handle: &fs::File) -> bool {
        #[link(name = "System")]
        unsafe extern "C" {
            fn acl_get_fd_np(descriptor: c_int, kind: c_int) -> *mut std::ffi::c_void;
            fn acl_free(object: *mut std::ffi::c_void) -> c_int;
        }
        const EXTENDED: c_int = 0x100; // ACL_TYPE_EXTENDED
        // An open descriptor; the result is null or a list this call owns and frees.
        let list = unsafe { acl_get_fd_np(handle.as_raw_fd(), EXTENDED) };
        if list.is_null() { return std::io::Error::last_os_error().raw_os_error() != Some(2); } // ENOENT: there is none
        unsafe { acl_free(list); }
        true
    }
    #[cfg(not(target_os = "macos"))]
    fn listed(_: &fs::File) -> bool { false }
    /// A directory passed through on the way: whoever can rename entries in it can put another
    /// directory under the next name. It must belong to root or the user, and be writable by
    /// nobody else unless it is sticky (`/private/tmp`), where only an entry's owner renames it.
    /// Its access control list is not read: macOS puts a deny-delete list on every home
    /// directory, and what is entered last is checked in full whatever stands above it.
    fn above(handle: &fs::File, owner: u32) -> std::result::Result<(), &'static str> {
        let found = handle.metadata().map_err(|_| NOT_FOUND)?;
        if (found.uid() != 0 && found.uid() != owner) || (found.mode() & 0o022 != 0 && found.mode() & 0o1000 == 0) { return Err(ABOVE); }
        Ok(())
    }
    pub(super) fn user() -> u32 {
        // Takes nothing, changes nothing, cannot fail.
        unsafe { geteuid() }
    }
    /// `name`, one path component, opened in `directory` without following a link.
    fn open(directory: &fs::File, name: &std::ffi::OsStr, flags: i32) -> std::io::Result<fs::File> {
        if name.is_empty() || name.as_bytes().contains(&b'/') { return Err(std::io::ErrorKind::InvalidInput.into()); }
        let name = CString::new(name.as_bytes())?;
        // `directory` is open and `name` is a NUL-terminated string that lives across the call;
        // the mode is read only when the flags create the file.
        let descriptor = unsafe { openat(directory.as_raw_fd(), name.as_ptr(), flags | flag::NOFOLLOW | flag::CLOEXEC, 0o600 as c_uint) };
        if descriptor < 0 { return Err(std::io::Error::last_os_error()); }
        // A descriptor just returned to this call, owned by nothing else.
        Ok(unsafe { fs::File::from_raw_fd(descriptor) })
    }
    /// What the handle is open on must be the owner's alone to write.
    fn checked(handle: &fs::File, owner: u32, directory: bool) -> std::result::Result<(), &'static str> {
        let Ok(found) = handle.metadata() else { return Err(if directory { NOT_FOUND } else { FILE_UNREADABLE }) };
        if directory != found.is_dir() || !(directory || found.is_file()) { return Err(if directory { NOT_FOUND } else { FILE_LINKED }); }
        if found.uid() != owner { return Err(if directory { NOT_OWNED } else { FILE_NOT_OWNED }); }
        if found.mode() & 0o022 != 0 { return Err(if directory { WRITABLE } else { FILE_WRITABLE }); }
        if listed(handle) { return Err(if directory { LISTED } else { FILE_LISTED }); }
        Ok(())
    }
    impl Directory {
        /// The directory an absolute path names, entered from `/` one component at a time, none
        /// of them a link and each one above it safe to pass through (`above`); `owner` must own it
        /// and be the only one who can write it.
        pub(super) fn open(path: &Path, owner: u32) -> std::result::Result<Self, &'static str> {
            if !path.is_absolute() { return Err(NOT_FOUND); }
            let mut handle = fs::File::open("/").map_err(|_| NOT_FOUND)?;
            let mut walked = std::path::PathBuf::from("/");
            for component in path.components() {
                let name = match component { Component::RootDir | Component::CurDir => continue, Component::Normal(name) => name, _ => return Err(NOT_FOUND) };
                above(&handle, owner)?;
                walked.push(name);
                handle = open(&handle, name, flag::DIRECTORY).map_err(|_| {
                    // Only to say why: the refusal is the failed open above.
                    if fs::symlink_metadata(&walked).is_ok_and(|found| found.file_type().is_symlink()) { LINKED } else { NOT_FOUND }
                })?;
            }
            checked(&handle, owner, true)?;
            Ok(Self { handle, owner })
        }
        /// A file of the directory, checked on the handle returned. A missing one is `None` when read.
        pub(super) fn file(&self, name: &str, access: Access) -> std::result::Result<Option<fs::File>, &'static str> {
            // Not blocking: opening a FIFO planted under the name must not hang the build.
            let flags = flag::NONBLOCK | match access { Access::Read => 0, Access::Append => flag::WRITE | flag::APPEND | flag::CREATE, Access::Lock => flag::READ_WRITE | flag::CREATE };
            let file = match open(&self.handle, name.as_ref(), flags) {
                Ok(file) => file,
                Err(e) if matches!(access, Access::Read) && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                // The name is a link (`O_NOFOLLOW`), or the open failed some other way.
                Err(e) => return Err(if e.raw_os_error() == Some(flag::ELOOP) { FILE_LINKED } else { FILE_UNREADABLE }),
            };
            checked(&file, self.owner, false)?;
            Ok(Some(file))
        }
        /// A directory inside this one, made for the owner alone if missing, under the same rules.
        pub(super) fn inside(&self, name: &str) -> Option<Self> {
            let text = CString::new(name).ok()?;
            // An open directory and a NUL-terminated name; an existing entry is left as it is.
            unsafe { mkdirat(self.handle.as_raw_fd(), text.as_ptr(), 0o700); }
            let handle = open(&self.handle, name.as_ref(), flag::DIRECTORY).ok()?;
            checked(&handle, self.owner, true).ok()?;
            Some(Self { handle, owner: self.owner })
        }
    }
}
#[cfg(not(all(unix, any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))))]
mod handle {
    use super::*;
    const UNSUPPORTED: &str = "WITCHY_PROOF_CACHE is refused: this system has no way here to check who can write the directory";
    pub(super) fn user() -> u32 { 0 }
    impl Directory {
        pub(super) fn open(_: &Path, _: u32) -> std::result::Result<Self, &'static str> { Err(UNSUPPORTED) }
        pub(super) fn file(&self, _: &str, _: Access) -> std::result::Result<Option<fs::File>, &'static str> { Err(UNSUPPORTED) }
        pub(super) fn inside(&self, _: &str) -> Option<Self> { None }
    }
}
pub(super) struct Cache { directory: Directory, binding: String, library_scopes: HashMap<String, String>, known: RefCell<Option<HashMap<String, String>>>, refused: RefCell<Option<HashMap<String, String>>> }
/// Whether every name of an `axioms=` list is in the allowlist.
fn allowlisted(axioms: &str) -> bool { axioms.split(',').filter(|name| !name.is_empty()).all(|name| AXIOMS.contains(&name)) }
/// The names of an `axioms=` list as JSON strings, or `None` if one is outside the allowlist.
fn allowed(axioms: &str) -> Option<String> {
    allowlisted(axioms).then(|| axioms.split(',').filter(|name| !name.is_empty()).map(|name| format!("\"{name}\"")).collect::<Vec<_>>().join(","))
}
/// A line of the accepted file: exactly a key, one space and an allowlisted `axioms=` list.
/// Checked without building the list, for every line of the file on each build's first lookup.
fn line(text: &str) -> Option<(&str, &str)> {
    let (key, rest) = text.split_once(' ')?;
    let axioms = rest.strip_prefix("axioms=")?;
    (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) && allowlisted(axioms)).then_some((key, axioms))
}
impl Cache {
    /// `WITCHY_PROOF_CACHE` names an existing absolute directory that only the user running the
    /// compiler can write (the module's note). Without it, or without a selected checker binding
    /// to tie entries to, every goal is checked afresh. A directory that fails the rules is an
    /// error, not a silent fresh check: the operator asked for a cache that cannot be trusted.
    pub(super) fn configured(binding: Option<&str>) -> Result<Option<Self>> {
        let (Some(directory), Some(binding)) = (std::env::var_os("WITCHY_PROOF_CACHE"), binding) else { return Ok(None) };
        Self::at(Path::new(&directory), binding, handle::user()).map(Some)
    }
    fn at(directory: &Path, binding: &str, owner: u32) -> Result<Self> {
        let directory = Directory::open(directory, owner).map_err(error)?;
        Ok(Self { directory, binding: binding.into(), library_scopes: HashMap::new(), known: RefCell::new(None), refused: RefCell::new(None) })
    }
    /// A file's text, or nothing when there is none; a file that fails the rules is an error.
    fn text(&self, name: &str) -> Result<String> {
        let Some(mut file) = self.directory.file(name, Access::Read).map_err(error)? else { return Ok(String::new()) };
        let mut text = String::new();
        // Unreadable as text: no entries, as before.
        if file.read_to_string(&mut text).is_err() { text.clear(); }
        Ok(text)
    }
    /// Adds whole lines to a file in one write.
    fn add(&self, name: &str, lines: &str) -> Result<()> {
        let mut file = self.directory.file(name, Access::Append).map_err(error)?.ok_or_else(|| error("proof cache write refused"))?;
        file.write_all(lines.as_bytes()).map_err(|_| error("proof cache write refused"))
    }
    pub(super) fn binding(&self) -> &str { &self.binding }
    /// Bind imported function evidence to its source and transitive import inputs.
    pub(super) fn bind_libraries(&mut self, program: &crate::ast::Program, source: &str, model_id: &str) -> Result<()> {
        use std::collections::BTreeSet;

        let mut imports = vec![Vec::new(); program.starts.len()];
        let mut imported = BTreeSet::new();
        for &(from, _, to) in &program.imports {
            imports[from as usize].push(to as usize);
            imported.insert(to as usize);
        }
        let mut scopes = HashMap::new();
        for declaration in &program.declarations {
            let crate::ast::Declaration::Function(function) = declaration else { continue };
            let module = function.module as usize;
            if !imported.contains(&module) {
                continue;
            }
            let scope = match scopes.entry(module) {
                std::collections::hash_map::Entry::Occupied(scope) => scope.into_mut(),
                std::collections::hash_map::Entry::Vacant(scope) => {
                    let mut reachable = BTreeSet::new();
                    let mut pending = vec![module];
                    while let Some(index) = pending.pop() {
                        if reachable.insert(index) {
                            pending.extend(imports[index].iter().copied());
                        }
                    }
                    let mut fingerprints = Vec::with_capacity(reachable.len() + 1);
                    fingerprints.push(model_id.to_owned());
                    for index in reachable {
                        let start = program.starts[index];
                        let end = program.starts.get(index + 1).copied().unwrap_or(source.len());
                        let content = source.get(start..end).ok_or_else(|| error("proof library source range refused"))?;
                        fingerprints.push(digest::bytes(content.as_bytes()).map_err(|_| error("proof library identity I/O"))?);
                    }
                    scope.insert(fingerprints.join(":"))
                }
            };
            self.library_scopes.insert(function.name.clone(), scope.clone());
        }
        Ok(())
    }

    pub(super) fn library_scope(&self, goal: &Goal) -> Option<&str> {
        goal.source.as_ref().and_then(|identity| self.library_scopes.get(&identity.function)).map(String::as_str)
    }

    /// The goal exactly as Lean would see it alone, with the audit and the checker identity.
    fn key(&self, goal: &Goal) -> Result<String> {
        let module = super::proof_module::emit(super::proof_module::Namespace::Current, &[goal], None)?.audited("WITCHY_PROOF_CACHE_V1")?;
        let scope = self.library_scope(goal);
        let key = match scope {
            Some(scope) => format!("{SCHEMA}.library\0{}\0{scope}\0{module}", self.binding),
            None => format!("{SCHEMA}\0{}\0{module}", self.binding),
        };
        digest::bytes(key.as_bytes()).map_err(|_| error("proof cache key I/O"))
    }
    /// Coordinate cold checks by their existing cryptographic proof keys. File
    /// locks carry no proof authority. On unsupported filesystems, lock errors,
    /// or a stalled peer, check the outstanding goals normally. Sorted keys
    /// prevent deadlocks between partially overlapping builds.
    pub(super) fn reserve(&self, goals: &[Goal], found: &mut [Found]) -> Result<Vec<fs::File>> {
        use std::{collections::BTreeSet, fs::TryLockError, time::{Duration, Instant}};
        let mut keys = BTreeSet::new();
        for (goal, status) in goals.iter().zip(found.iter()) {
            if matches!(status, Found::Missing) { keys.insert(self.key(goal)?); }
        }
        if keys.is_empty() { return Ok(Vec::new()); }
        let Some(dir) = self.directory.inside("in-flight") else { return Ok(Vec::new()) };
        let started = Instant::now();
        let mut locks = Vec::new();
        for key in keys {
            let Ok(Some(file)) = dir.file(&key, Access::Lock) else { return Ok(Vec::new()); };
            loop {
                match file.try_lock() {
                    Ok(()) => break,
                    Err(TryLockError::WouldBlock) if started.elapsed() < Duration::from_secs(10) =>
                        std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => return Ok(Vec::new()),
                }
            }
            locks.push(file);
        }
        // An owner may finish between our first read and an uncontended lock.
        // Only accepted/refused journal entries can answer the second lookup.
        self.known.replace(None);
        self.refused.replace(None);
        for (goal, status) in goals.iter().zip(found.iter_mut()) {
            if matches!(status, Found::Missing) { *status = self.find(goal)?; }
        }
        Ok(locks)
    }
    /// The accepted file, read on first use. A line that is not exactly an entry is skipped.
    fn known(&self) -> Result<std::cell::RefMut<'_, HashMap<String, String>>> {
        let mut known = self.known.borrow_mut();
        if known.is_none() {
            let text = self.text(ACCEPTED)?;
            *known = Some(text.lines().filter_map(line).map(|(key, axioms)| (key.to_owned(), axioms.to_owned())).collect());
        }
        Ok(std::cell::RefMut::map(known, |known| known.as_mut().expect("read above")))
    }
    /// Adds whole lines in one write, so another compiler reading or appending never sees part of one.
    fn append(&self, entries: &[(String, String)]) -> Result<()> {
        let text = entries.iter().map(|(key, axioms)| format!("{key} axioms={axioms}\n")).collect::<String>();
        self.add(ACCEPTED, &text)?;
        self.known()?.extend(entries.iter().cloned());
        Ok(())
    }
    /// An entry in the earlier layout: a file named by the key, holding the schema, the key and the axioms.
    fn earlier(&self, key: &str) -> Result<Option<String>> {
        let text = self.text(key)?;
        let mut lines = text.lines();
        if lines.next() != Some(SCHEMA) || lines.next() != Some(key) { return Ok(None); }
        let Some(axioms) = lines.next().and_then(|line| line.strip_prefix("axioms=")) else { return Ok(None) };
        Ok((lines.next().is_none() && allowed(axioms).is_some()).then(|| axioms.to_owned()))
    }
    /// What the cache holds for a goal, from one key (a build asked for the refusal and then the
    /// acceptance, emitting and hashing each goal's Lean module twice: 2026-11-19, 11% of a
    /// warm hello-world build). A refusal is reported first, as before.
    pub(super) fn find(&self, goal: &Goal) -> Result<Found> {
        let key = self.key(goal)?;
        if let Some(message) = self.refusals()?.get(&key).cloned() { return Ok(Found::Refused(message)); }
        Ok(match self.lookup_key(key)? { Some(axioms) => Found::Accepted(axioms), None => Found::Missing })
    }
    /// The axioms the cached proof depends on, as JSON strings, if this goal was accepted before.
    #[cfg(test)]
    pub(super) fn lookup(&self, goal: &Goal) -> Result<Option<String>> {
        let key = self.key(goal)?;
        self.lookup_key(key)
    }
    fn lookup_key(&self, key: String) -> Result<Option<String>> {
        if let Some(axioms) = self.known()?.get(&key) { return Ok(allowed(axioms)); }
        let Some(axioms) = self.earlier(&key)? else { return Ok(None) };
        // Copied over so the next build finds it without opening this file. Failing to is not an error.
        let _ = self.append(&[(key, axioms.clone())]);
        Ok(allowed(&axioms))
    }
    /// The refused file, read on first use: lines of a key, one space and a one-line text.
    fn refusals(&self) -> Result<std::cell::RefMut<'_, HashMap<String, String>>> {
        let mut refused = self.refused.borrow_mut();
        if refused.is_none() {
            let text = self.text(REFUSED)?;
            *refused = Some(text.lines().filter_map(|line| line.split_once(' ')).filter(|(key, _)| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(|(key, text)| (key.to_owned(), text.to_owned())).collect());
        }
        Ok(std::cell::RefMut::map(refused, |refused| refused.as_mut().expect("read above")))
    }
    /// Failing to write, or a file that fails the rules, keeps nothing: a refusal is only a saving.
    fn note(&self, key: String, text: &str) {
        let text = text.replace(['\n', '\r'], " ");
        let line = format!("{key} {text}\n");
        if self.add(REFUSED, &line).is_ok() && let Ok(mut refusals) = self.refusals() { refusals.insert(key, text); }
    }
    /// The message Lean gave when it refused this goal before, if it did.
    #[cfg(test)]
    pub(super) fn refusal(&self, goal: &Goal) -> Result<Option<String>> {
        let key = self.key(goal)?;
        Ok(self.refusals()?.get(&key).cloned())
    }
    /// Records a goal Lean refused, with its message. Failing to write is not an error.
    pub(super) fn store_refusal(&self, goal: &Goal, message: &str) -> Result<()> {
        let key = self.key(goal)?;
        self.note(key, message);
        Ok(())
    }
    fn explanation_key(&self, module: &str) -> Option<String> {
        // Version 2 records results from the binding used to select this key.
        digest::bytes(format!("witchy.proof-explanation.v2\0{}\0{module}", self.binding).as_bytes()).ok()
    }
    /// What an explanation's request found before: the goals in it that failed.
    pub(super) fn explained(&self, module: &str, goal_count: usize) -> Option<Vec<usize>> {
        let key = self.explanation_key(module)?;
        let text = self.refusals().ok()?.get(&key).cloned()?;
        if text == "-" { return Some(Vec::new()); }
        text.split(',').map(|n| n.parse().ok().filter(|&index| index < goal_count)).collect()
    }
    pub(super) fn store_explained(&self, module: &str, failed: &[usize]) {
        let Some(key) = self.explanation_key(module) else { return };
        let text = if failed.is_empty() { "-".to_owned() } else { failed.iter().map(usize::to_string).collect::<Vec<_>>().join(",") };
        self.note(key, &text);
    }
    /// Records goals a request just passed. `dependencies` is the audit of that request, in goal order.
    pub(super) fn store(&self, goals: &[Goal], dependencies: &str) -> Result<()> {
        let audits = dependencies.split("\"axioms\":[").skip(1).map(|rest| rest.split(']').next().unwrap_or("")).collect::<Vec<_>>();
        if audits.len() != goals.len() { return Err(error("proof cache: audit does not cover every goal")); }
        let mut entries = Vec::new();
        for (goal, axioms) in goals.iter().zip(audits) {
            let names = axioms.split(',').map(|name| name.trim_matches('"')).filter(|name| !name.is_empty()).collect::<Vec<_>>();
            if names.iter().any(|name| !AXIOMS.contains(name)) { return Err(error("proof cache: audit names an axiom outside the allowlist")); }
            entries.push((self.key(goal)?, names.join(",")));
        }
        self.append(&entries)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::{PermissionsExt, symlink}, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("witchy-proof-cache-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(&path).unwrap();
            // By its real path (`/tmp` and `/var` are links on macOS), and the owner's alone whatever the umask.
            let path = fs::canonicalize(path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn cache(&self, binding: &str) -> Cache { self.open(binding).unwrap() }
        fn open(&self, binding: &str) -> Result<Cache> { Cache::at(&self.0, binding, handle::user()) }
        fn refusal(&self) -> String { self.open("binding-a").err().expect("refused").message.to_string() }
    }
    impl Drop for Scratch { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    fn goal(statement: &str, prover: Option<&str>) -> Goal {
        Goal { source_name: "f".into(), statement: statement.into(), nodes: 1, source: None, prover: prover.map(str::to_owned) }
    }
    const AUDIT: &str = "[{\"theorem\":\"WitchyScalar.goal_0\",\"axioms\":[\"propext\",\"Quot.sound\"]},{\"theorem\":\"WitchyScalar.goal_1\",\"axioms\":[]}]";
    #[test]
    fn a_stale_miss_is_rechecked_after_reserving_the_goal() {
        let scratch = Scratch::new();
        let other = scratch.cache("checker");
        let first = scratch.cache("checker");
        let goals = [goal("True", None)];
        let mut stale = [other.find(&goals[0]).unwrap()];
        let locks = first.reserve(&goals, &mut [Found::Missing]).unwrap();
        assert_eq!(locks.len(), 1);
        first.store(&goals, r#"[{"theorem":"WitchyScalar.goal_0","axioms":[]}]"#).unwrap();
        drop(locks);
        let _locks = other.reserve(&goals, &mut stale).unwrap();
        assert!(matches!(&stale[0], Found::Accepted(axioms) if axioms.is_empty()));
    }
    #[test]
    fn a_stored_goal_is_found_with_its_axioms_and_nothing_else_is(){
        let scratch = Scratch::new(); let cache = scratch.cache("binding-a");
        let (first, second) = (goal("1 = 1", None), goal("2 = 2", None));
        assert_eq!(cache.lookup(&first).unwrap(), None);
        cache.store(&[goal("1 = 1", None), goal("2 = 2", None)], AUDIT).unwrap();
        assert_eq!(cache.lookup(&first).unwrap().as_deref(), Some("\"propext\",\"Quot.sound\""));
        assert_eq!(cache.lookup(&second).unwrap().as_deref(), Some(""));
        // Another statement, another proof script, or another checker is a miss.
        assert_eq!(cache.lookup(&goal("3 = 3", None)).unwrap(), None);
        assert_eq!(cache.lookup(&goal("1 = 1", Some("  simp\n"))).unwrap(), None);
        assert_eq!(scratch.cache("binding-b").lookup(&first).unwrap(), None);
    }
    #[test]
    fn a_later_build_reads_what_an_earlier_one_stored_from_one_file(){
        let scratch = Scratch::new();
        scratch.cache("binding-a").store(&[goal("1 = 1", None), goal("2 = 2", None)], AUDIT).unwrap();
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
        let later = scratch.cache("binding-a");
        assert_eq!(later.lookup(&goal("1 = 1", None)).unwrap().as_deref(), Some("\"propext\",\"Quot.sound\""));
        assert_eq!(later.lookup(&goal("2 = 2", None)).unwrap().as_deref(), Some(""));
    }
    #[test]
    fn a_damaged_line_is_a_miss(){
        let scratch = Scratch::new();
        let one = goal("1 = 1", None);
        let key = scratch.cache("binding-a").key(&one).unwrap();
        let good = format!("{key} axioms=propext\n");
        for bad in [String::new(), good.replace("axioms=", "axioms=sorryAx,"), good.replace(" axioms", "  axioms"), good.replace("axioms=", "audit="),
            good.replacen(&key, &"0".repeat(64), 1), good.replacen(&key, &key[1..], 1), format!("{} extra\n", good.trim_end())] {
            fs::write(scratch.0.join(ACCEPTED), &bad).unwrap();
            assert_eq!(scratch.cache("binding-a").lookup(&one).unwrap(), None, "{bad:?}");
        }
        fs::write(scratch.0.join(ACCEPTED), format!("not an entry\n{good}")).unwrap();
        assert_eq!(scratch.cache("binding-a").lookup(&one).unwrap().as_deref(), Some("\"propext\""));
    }
    #[test]
    fn an_entry_in_the_earlier_layout_is_found_copied_over_and_damaged_ones_are_misses(){
        let scratch = Scratch::new(); let cache = scratch.cache("binding-a");
        let one = goal("1 = 1", None);
        let key = cache.key(&one).unwrap();
        let path = scratch.0.join(&key);
        let good = format!("{SCHEMA}\n{key}\naxioms=Quot.sound\n");
        for bad in [String::new(), good.replace(SCHEMA, "witchy.proof-cache.v0"), good.replace("axioms=", "axioms=sorryAx"), format!("{good}extra\n"), good.replacen(&key, &"0".repeat(64), 1)] {
            fs::write(&path, bad).unwrap();
            assert_eq!(cache.lookup(&one).unwrap(), None);
        }
        fs::write(&path, good).unwrap();
        assert_eq!(cache.lookup(&one).unwrap().as_deref(), Some("\"Quot.sound\""));
        fs::remove_file(&path).unwrap();
        assert_eq!(scratch.cache("binding-a").lookup(&one).unwrap().as_deref(), Some("\"Quot.sound\""), "read from the one file now");
    }
    #[test]
    fn find_answers_from_one_key_and_a_refusal_comes_first(){
        let scratch = Scratch::new(); let cache = scratch.cache("binding-a");
        let pair = [goal("1 = 1", None), goal("2 = 2", None)];
        let (one, two, three) = (&pair[0], &pair[1], &goal("1 = 2", None));
        assert!(matches!(cache.find(one).unwrap(), Found::Missing));
        cache.store(&pair, r#"{"axioms":["propext"]},{"axioms":[]}"#).unwrap();
        assert!(matches!(cache.find(one).unwrap(), Found::Accepted(a) if a == "\"propext\""));
        assert!(matches!(cache.find(two).unwrap(), Found::Accepted(a) if a.is_empty()));
        cache.store_refusal(three, "no").unwrap();
        assert!(matches!(cache.find(three).unwrap(), Found::Refused(m) if m == "no"));
        // A goal both accepted and refused (two checkers' records in one directory) is refused, as before.
        cache.store_refusal(one, "later").unwrap();
        assert!(matches!(scratch.cache("binding-a").find(one).unwrap(), Found::Refused(m) if m == "later"));
    }
    #[test]
    fn a_refusal_is_kept_apart_from_acceptance_and_read_by_a_later_build(){
        let scratch = Scratch::new(); let cache = scratch.cache("binding-a");
        let one = goal("1 = 2", None);
        assert_eq!(cache.refusal(&one).unwrap(), None);
        cache.store_refusal(&one, "unsolved goals\nmore").unwrap();
        assert_eq!(cache.lookup(&one).unwrap(), None, "a refusal is never an acceptance");
        let later = scratch.cache("binding-a");
        assert_eq!(later.refusal(&one).unwrap().as_deref(), Some("unsolved goals more"));
        assert_eq!(scratch.cache("binding-b").refusal(&one).unwrap(), None);
        later.store_explained("module text", &[0, 2]);
        later.store_explained("other text", &[]);
        let again = scratch.cache("binding-a");
        assert_eq!(again.explained("module text", 3), Some(vec![0, 2]));
        assert_eq!(again.explained("module text", 2), None);
        assert_eq!(again.explained("other text", 1), Some(vec![]));
        assert_eq!(again.explained("unseen", 1), None);
        assert_eq!(scratch.cache("binding-b").explained("module text", 3), None);
    }
    #[test]
    fn only_a_complete_allowlisted_audit_is_stored(){
        let scratch = Scratch::new(); let cache = scratch.cache("binding-a");
        assert!(cache.store(&[goal("1 = 1", None), goal("2 = 2", None)], "[{\"theorem\":\"WitchyScalar.goal_0\",\"axioms\":[]}]").is_err());
        assert!(cache.store(&[goal("1 = 1", None)], "[{\"theorem\":\"WitchyScalar.goal_0\",\"axioms\":[\"sorryAx\"]}]").is_err());
        assert_eq!(cache.lookup(&goal("1 = 1", None)).unwrap(), None);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
    const FORGED: &str = "axioms=\n";
    /// Review 2026-11-21 (`proofpipe/cache-forged`): whoever can write the directory can claim a
    /// proof, so a directory others can write, or that another user owns, is not used at all.
    #[test]
    fn a_directory_others_can_write_or_another_user_owns_is_refused() {
        let scratch = Scratch::new();
        let one = goal("1 = 2", None);
        let key = scratch.cache("binding-a").key(&one).unwrap();
        fs::write(scratch.0.join(ACCEPTED), format!("{key} {FORGED}")).unwrap();
        fs::set_permissions(scratch.0.join(ACCEPTED), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(scratch.cache("binding-a").lookup(&one).unwrap().as_deref(), Some(""), "the control: the owner's own directory is trusted");
        for (mode, why) in [(0o770, "writable by its group or by others"), (0o707, "writable by its group or by others"), (0o777, "writable by its group or by others"), (0o1777, "writable by its group or by others")] {
            fs::set_permissions(&scratch.0, fs::Permissions::from_mode(mode)).unwrap();
            assert!(scratch.refusal().contains(why), "{mode:o}: {}", scratch.refusal());
        }
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(scratch.open("binding-a").is_ok(), "readable by others is not writable by them");
        let other = Cache::at(&scratch.0, "binding-a", handle::user() + 1).err().expect("another owner").message;
        // Asked as a user who owns neither it nor the directories above it (those are seen first).
        assert!(other.contains("not owned by the user running the compiler") || other.contains("belongs to another user"), "{other}");
    }
    #[test]
    fn a_directory_reached_through_a_link_or_not_a_directory_is_refused() {
        let scratch = Scratch::new();
        let real = scratch.0.join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(real.join("inner")).unwrap();
        fs::set_permissions(real.join("inner"), fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&real, scratch.0.join("link")).unwrap();
        let refusal = |path: PathBuf| Cache::at(&path, "binding-a", handle::user()).err().map(|e| e.message.to_string());
        assert_eq!(refusal(real.clone()), None);
        assert_eq!(refusal(real.join("inner")), None);
        // The directory itself, and a directory above it.
        assert!(refusal(scratch.0.join("link")).expect("final link").contains("symbolic link"));
        assert!(refusal(scratch.0.join("link/inner")).expect("link above").contains("symbolic link"));
        assert_eq!(refusal(real.join("../real")).as_deref(), Some(NOT_FOUND));
        assert_eq!(refusal(real.join("missing")).as_deref(), Some(NOT_FOUND));
        assert_eq!(refusal(PathBuf::from("relative")).as_deref(), Some(NOT_FOUND));
        fs::write(real.join("file"), "").unwrap();
        assert_eq!(refusal(real.join("file")).as_deref(), Some(NOT_FOUND));
    }
    /// The accepted file, the refused file and an earlier-layout entry: each a regular file the
    /// user owns and alone can write, or the build is refused; nothing is read from or added to it.
    #[test]
    fn a_file_that_is_a_link_or_others_can_write_is_refused() {
        let one = goal("1 = 2", None);
        for name in [ACCEPTED, REFUSED, "earlier"] {
            let scratch = Scratch::new();
            let key = scratch.cache("binding-a").key(&one).unwrap();
            let name = if name == "earlier" { key.as_str() } else { name };
            let text = if name == REFUSED { format!("{key} no\n") } else if name == ACCEPTED { format!("{key} {FORGED}") } else { format!("{SCHEMA}\n{key}\n{FORGED}") };
            let find = || scratch.cache("binding-a").find(&one).map(|found| matches!(found, Found::Missing)).map_err(|e| e.message.to_string());
            // A link to a file elsewhere, even one that passes every other rule.
            let elsewhere = Scratch::new();
            fs::write(elsewhere.0.join("planted"), &text).unwrap();
            fs::set_permissions(elsewhere.0.join("planted"), fs::Permissions::from_mode(0o600)).unwrap();
            symlink(elsewhere.0.join("planted"), scratch.0.join(name)).unwrap();
            assert!(find().expect_err(name).contains("symbolic link or not a regular file"), "{name}");
            assert!(scratch.cache("binding-a").store(&[goal("1 = 1", None)], r#"{"axioms":[]}"#).is_err() || name != ACCEPTED, "nothing is added through a link");
            assert_eq!(fs::read_to_string(elsewhere.0.join("planted")).unwrap(), text, "{name}");
            fs::remove_file(scratch.0.join(name)).unwrap();
            // A directory under the name.
            fs::create_dir(scratch.0.join(name)).unwrap();
            assert!(find().expect_err(name).contains("not a regular file"), "{name}");
            fs::remove_dir(scratch.0.join(name)).unwrap();
            // The file itself, writable by its group or by others; then another owner's.
            fs::write(scratch.0.join(name), &text).unwrap();
            for mode in [0o620, 0o602, 0o666] {
                fs::set_permissions(scratch.0.join(name), fs::Permissions::from_mode(mode)).unwrap();
                assert!(find().expect_err(name).contains("writable by its group or by others"), "{name} {mode:o}");
            }
            fs::set_permissions(scratch.0.join(name), fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(find(), Ok(false), "{name}: the owner's own file is read");
            let other = Cache::at(&scratch.0, "binding-a", handle::user() + 1);
            assert!(other.is_err(), "another owner's directory");
            let mut mine = scratch.cache("binding-a");
            mine.directory.owner += 1;
            assert!(mine.find(&one).err().expect("another owner's file").message.contains("not owned by the user"), "{name}");
        }
    }
    /// The handle is the directory: renaming it and putting another under its name changes nothing.
    #[test]
    fn the_directory_opened_is_the_one_used() {
        let scratch = Scratch::new();
        let (first, moved) = (scratch.0.join("cache"), scratch.0.join("moved"));
        fs::create_dir(&first).unwrap();
        fs::set_permissions(&first, fs::Permissions::from_mode(0o700)).unwrap();
        let cache = Cache::at(&first, "binding-a", handle::user()).unwrap();
        let one = goal("1 = 2", None);
        let key = cache.key(&one).unwrap();
        fs::rename(&first, &moved).unwrap();
        fs::create_dir(&first).unwrap();
        fs::write(first.join(ACCEPTED), format!("{key} {FORGED}")).unwrap();
        assert!(matches!(cache.find(&one).unwrap(), Found::Missing));
        cache.store(&[goal("1 = 1", None)], r#"{"axioms":[]}"#).unwrap();
        assert!(moved.join(ACCEPTED).exists(), "stored in the directory that was opened");
        assert_eq!(fs::read_to_string(first.join(ACCEPTED)).unwrap(), format!("{key} {FORGED}"), "the directory now under the name is untouched");
    }
    /// A lock is no authority, but its directory is entered the same way: a link there means no
    /// coordination, and nothing is made through it.
    #[test]
    fn a_linked_lock_directory_is_not_entered() {
        let (scratch, elsewhere) = (Scratch::new(), Scratch::new());
        symlink(&elsewhere.0, scratch.0.join("in-flight")).unwrap();
        let cache = scratch.cache("binding-a");
        let goals = [goal("True", None)];
        assert!(cache.reserve(&goals, &mut [Found::Missing]).unwrap().is_empty());
        assert_eq!(fs::read_dir(&elsewhere.0).unwrap().count(), 0);
    }
    /// A directory above the cache that others can write lets them put another directory under
    /// its name: refused unless it is sticky (as `/private/tmp` is), or another user's.
    #[test]
    fn a_directory_above_that_others_can_write_is_refused_unless_sticky() {
        let scratch = Scratch::new();
        let (open, cache) = (scratch.0.join("open"), scratch.0.join("open/cache"));
        fs::create_dir(&open).unwrap();
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
        let refusal = |owner: u32| Cache::at(&cache, "binding-a", owner).err().map(|e| e.message.to_string());
        for mode in [0o777, 0o775, 0o757] {
            fs::set_permissions(&open, fs::Permissions::from_mode(mode)).unwrap();
            assert!(refusal(handle::user()).expect("writable above").contains("a directory above it"), "{mode:o}");
        }
        fs::set_permissions(&open, fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(refusal(handle::user()), None, "sticky: only an entry's owner renames it");
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(refusal(handle::user()), None);
    }
    /// macOS: an access control list grants writing without changing the modes, so the directory,
    /// `accepted.v1` and `refused.v1` are refused when they carry one at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_access_control_list_is_refused() {
        let grant = |path: &Path, right: &str| std::process::Command::new("/bin/chmod").arg("+a").arg(format!("everyone allow {right}")).arg(path).status().is_ok_and(|s| s.success());
        let probe = Scratch::new();
        if !grant(&probe.0, "add_file") {
            eprintln!("skipped: `/bin/chmod +a` cannot set an access control list here (not macOS chmod, or a filesystem without them)");
            return;
        }
        assert!(probe.refusal().contains("access control list"), "{}", probe.refusal());
        // Also a list that only denies: any list at all.
        let denied = Scratch::new();
        assert!(std::process::Command::new("/bin/chmod").args(["+a", "everyone deny delete"]).arg(&denied.0).status().unwrap().success());
        assert!(denied.refusal().contains("access control list"));
        let one = goal("1 = 2", None);
        for name in [ACCEPTED, REFUSED] {
            let scratch = Scratch::new();
            let key = scratch.cache("binding-a").key(&one).unwrap();
            fs::write(scratch.0.join(name), if name == ACCEPTED { format!("{key} {FORGED}") } else { format!("{key} no\n") }).unwrap();
            fs::set_permissions(scratch.0.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            assert!(scratch.cache("binding-a").find(&one).is_ok(), "{name}: the control");
            assert!(grant(&scratch.0.join(name), "write"), "{name}");
            let refused = scratch.cache("binding-a").find(&one).err().expect(name).message;
            assert!(refused.contains("one of its files carries an access control list"), "{name}: {refused}");
            assert!(scratch.cache("binding-a").store(&[goal("1 = 1", None)], r#"{"axioms":[]}"#).is_err() || name != ACCEPTED, "nothing is added to it");
        }
    }
}
